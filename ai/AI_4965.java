import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_4965 extends AI {
public AI_4965() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
session.rebindAll(this);
var u_a = session.getVariable("a");
var u_m = session.getVariable("m");
var u_t = session.getVariable("t");
Object u_b = ops(get(u_t.get(), 0l, null), 1);
session.setVariable(AI_4965.this, "b", u_b);
return null;
}
protected String getAIString() { return "<snippet 4965>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 4965>", };}

protected int[] getErrorFilesID() { return new int[] {4965, };}

}
