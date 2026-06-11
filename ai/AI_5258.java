import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_5258 extends AI {
public AI_5258() throws LeekRunException {
super(3, 1);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
var u_i = new Box<Object>(AI_5258.this, new SetLeekValue(AI_5258.this, new Object[] { 1l, 2l }), 4);
ops(Set_setPut_hx(load(u_i.get()), 3l), 3);
return u_i;
}
protected String getAIString() { return "<snippet 5258>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 5258>", };}

protected int[] getErrorFilesID() { return new int[] {5258, };}

private boolean Set_setPut_hx(Object a0, Object a1) throws LeekRunException {
SetLeekValue x0; try { x0 = (SetLeekValue) (a0); } catch (ClassCastException e) { return false; }
return x0.setPut(this, a1);
}

}
