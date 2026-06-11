import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_4961 extends AI {
public AI_4961() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
session.rebindAll(this);
var u_m = session.getVariable("m");
Object u_a = ops(get(u_m.get(), "a", null), 1);
session.setVariable(AI_4961.this, "a", u_a);
return null;
}
protected String getAIString() { return "<snippet 4961>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 4961>", };}

protected int[] getErrorFilesID() { return new int[] {4961, };}

}
