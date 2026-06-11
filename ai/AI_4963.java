import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_4963 extends AI {
public AI_4963() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
session.rebindAll(this);
var u_a = session.getVariable("a");
var u_m = session.getVariable("m");
var u_t = session.getVariable("t");
return ops(Array_push_ax(u_t.get(), 2l), 2);
}
protected String getAIString() { return "<snippet 4963>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 4963>", };}

protected int[] getErrorFilesID() { return new int[] {4963, };}

private Object Array_push_ax(Object a0, Object a1) throws LeekRunException {
ArrayLeekValue x0; try { x0 = toArray(1, a0); } catch (ClassCastException e) { return null; }
return x0.push(this, a1);
}

}
